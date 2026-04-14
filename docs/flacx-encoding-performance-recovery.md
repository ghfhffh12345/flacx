# flacx encode performance recovery notes

This maintainer note translates
`.omx/plans/prd-encoding-speed-15pct-api-stable.md` into a durable in-repo
review companion for the active encode-performance lane.

It exists because the plan file is an OMX execution artifact, while maintainers
still need a repo-local document that keeps the benchmark gate, hotspot review
cues, and documentation obligations visible during implementation.

## Non-negotiable guardrails

- keep the current external encode API stable
- preserve the reader-first encode flow built around `read_pcm_reader(...)`
  and `EncoderConfig::into_encoder(...)`
- keep `flacx::builtin` as a thin convenience layer over the same encode path
- do not add dependencies
- do not weaken compression-level or encoder-config semantics to buy speed
- document any encoded-size change instead of treating it as free

## Binding performance gate

The current planning baseline is the public library throughput bench:

```bash
cargo bench -p flacx --bench throughput -- --noplot
```

Planning baseline captured on **2026-04-14**:
- `encode_corpus_throughput` median ≈ `21.102 MiB/s`
- required completion target ≈ `24.267 MiB/s`

That gate is the right binding authority for this lane because the request is
explicitly about improving library encode speed **without changing the external
`flacx` API**.

## Grounded repo findings

### 1. The benchmark is authoritative, but it is not a pure encode-core microbench

Repo evidence from `crates/flacx/benches/throughput.rs`:
- the encode corpus currently consists of three generated WAV fixtures with
  `4_096`, `8_192`, and `16_384` frames
- each bench iteration creates a fresh temp directory and fresh output files
- the benchmark therefore folds setup, file creation, reader startup, and
  output-path work into the same score as encode-core work

Review implication:
- the plan is correct to require a measurement stage before hard-coding the
  first optimization wave
- a change that only helps long-stream chunking may miss the real binding gate
  if setup, scheduling, or write ordering dominates these small fixtures

### 2. API-stability guardrails already exist and should stay the first regression lane

Repo evidence from `crates/flacx/tests/api.rs`:
- builtin encode helpers are compared directly against the explicit reader /
  session flow
- configured encoder options are asserted through the explicit session path
- AIFF and CAF encode entry points are covered behind their feature gates
- the public API story is already exercised through `builtin`,
  `read_pcm_reader(...)`, and `EncoderConfig::into_encoder(...)`

Review implication:
- performance work should stay internal unless a regression test proves the
  public story drifted
- documentation updates should continue to describe the reader-first encode
  flow as the stable conceptual center

### 3. Full-stream buffering is a real hotspot candidate, but not the only credible one

Repo evidence from `crates/flacx/src/encode_pipeline.rs`:
- `encode_stream(...)` currently reads the full declared PCM stream into a
  single `Vec<i32>` before the encode phase proceeds
- the current chunk bounds are effectively `chunk_start = 0` and
  `chunk_end = plan.total_frames`
- `encode_chunk(...)` then fans frame work out from that buffered sample block

Review implication:
- reducing whole-stream materialization may be high leverage
- however, the plan is right not to precommit to chunking first without timing
  proof from the binding benchmark shape

### 4. Model and input allocation churn remain strong secondary suspects

Repo evidence:
- `crates/flacx/src/model.rs` still materializes multiple `Vec<i32>`-backed
  intermediates for channel extraction, warmup data, and residual storage
- `crates/flacx/src/wav_input.rs` and `crates/flacx/src/raw.rs` each decode
  chunks through temporary `Vec<i32>` allocation paths before the encode side
  consumes them
- `encode_pipeline.rs` still coordinates worker output through batched chunks,
  channel send/receive, and `BTreeMap`-based pending reordering

Review implication:
- the correct first fix may live in buffering, frame modeling, or chunk-order
  coordination
- the plan's stage-gated wording should be treated as a real constraint, not as
  soft prose

### 5. The documentation burden is part of the performance work, not a follow-up extra

The PRD already requires:
- a benchmark artifact in `.omx/reports/perf/encoding-speed-15pct-api-stable.md`
- an optional JSON companion
- a hotspot timing artifact explaining why Wave 1 was chosen
- an explicit note on whether the first measured wave cleared the full target

Review implication:
- implementation is not complete when the code gets faster but the measurement
  trail is missing
- reviewers should reject performance claims that do not preserve the benchmark
  command, median, target, and hotspot evidence together

## Documentation contract for the implementation lane

When this performance work lands, keep the documentation story in this order:

1. **Stable public API first**
   - `read_pcm_reader(...)`
   - `EncoderConfig::into_encoder(...)`
   - `flacx::builtin` remains convenience, not a separate engine
2. **Measured hotspot explanation second**
   - say which hotspot class actually dominated
   - say why that wave was chosen over the others
3. **Performance evidence third**
   - baseline command
   - final command
   - before/after medians
   - percent change
   - encoded-size note if relevant
4. **Regression proof fourth**
   - API tests
   - targeted encode tests
   - feature-matrix checks when family readers are touched

Do not describe a broad architecture rewrite if the actual change is a bounded
encode hot-path optimization.

## Review companions

Keep this note paired with:
- `.omx/plans/prd-encoding-speed-15pct-api-stable.md`
- `crates/flacx/benches/throughput.rs`
- `crates/flacx/tests/api.rs`
- `crates/flacx/src/encode_pipeline.rs`
- `crates/flacx/src/model.rs`
- `crates/flacx/src/wav_input.rs`
- `crates/flacx/src/raw.rs`
- generated artifacts under `.omx/reports/perf/`

## Exit criteria for this note

This note can shrink or disappear once all of the following are true:

1. the public encode API is still unchanged and verified
2. the throughput bench clears the `>=15%` target against the documented
   planning baseline
3. the winning hotspot choice is explained by a durable timing artifact
4. any encoded-size movement is recorded instead of implied away
