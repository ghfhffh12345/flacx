# flacx encode performance recovery notes

This maintainer note translates
`.omx/plans/prd-encoding-speed-10pct-identical-output-api-stable.md` into a
durable in-repo review companion for the active encode-performance lane.

It exists because the plan file is an OMX execution artifact, while maintainers
still need a repo-local document that keeps the binding throughput gate,
byte-identity guardrails, hotspot review cues, and documentation obligations
visible during implementation and review.

## Non-negotiable guardrails

- keep the current external encode API stable
- preserve the reader-first encode flow built around `read_pcm_reader(...)`
  and `EncoderConfig::into_encoder(...)`
- keep `flacx::builtin` as a thin convenience layer over the same encode path
- preserve encoded outputs byte-for-byte for the agreed reference
  corpus/configuration matrix instead of accepting decode-equivalent drift
- do not add dependencies
- do not weaken compression-level, frame-selection, or encoder-config semantics
  to buy speed
- document any encoded-size movement instead of treating it as free

## Binding performance gate

The current planning baseline is the public library throughput bench:

```bash
cargo bench -p flacx --bench throughput -- --noplot
```

Planning baseline captured on **2026-04-15**:
- `encode_corpus_throughput` median ≈ `22.666 MiB/s`
- required completion target ≈ `24.933 MiB/s`

That gate is the right binding authority for this lane because the request is
explicitly about improving library encode speed **without changing the external
`flacx` API and while keeping encoded output identical**.

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
- harness timing can inform hotspot choice, but benchmark-only tricks do not
  satisfy the request

### 2. API-stability and output-stability guardrails already exist and should stay first-class

Repo evidence from `crates/flacx/tests/api.rs` and `crates/flacx/tests/encode.rs`:
- builtin encode helpers are compared directly against the explicit reader /
  session flow
- configured encoder options are asserted through the explicit session path
- AIFF and CAF encode entry points are covered behind their feature gates
- `produces_identical_output_across_thread_counts` already protects a key
  deterministic-output invariant for the shared encode path
- variable block schedules, explicit block sizes, and metadata-preservation
  cases already have encode regression coverage

Review implication:
- performance work should stay internal unless a regression test proves the
  public story drifted
- the byte-identity requirement should widen the evidence lane, not relax it:
  existing determinism tests stay, and a current-vs-reference output artifact
  must be added for the agreed corpus/config matrix
- documentation updates should continue to describe the reader-first encode
  flow as the stable conceptual center

### 3. Full-stream buffering is still a real hotspot candidate, but not the only credible one

Repo evidence from `crates/flacx/src/encode_pipeline.rs`:
- `encode_stream(...)` currently reads the full declared PCM stream into a
  single `Vec<i32>` before the encode phase proceeds
- the current chunk bounds are effectively `chunk_start = 0` and
  `chunk_end = plan.total_frames`
- `encode_chunk(...)` then fans frame work out from that buffered sample block
- the pipeline still performs an ordered collection pass before
  `write_encoded_chunk(...)` commits frames

Review implication:
- reducing whole-stream materialization may be high leverage
- however, the plan is right not to precommit to chunking first without timing
  proof from the binding benchmark shape
- any buffering change must keep frame boundaries, MD5 patching order,
  metadata handling, and exact frame bytes stable

### 4. Model, input, and ordering allocation churn remain strong secondary suspects

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
- any change that alters candidate evaluation order or residual materialization
  needs immediate byte-identity revalidation, not just decode success

### 5. Keep the explicit encode session as the single public story

Repo evidence from `crates/flacx/src/encoder.rs`:
- the public encode path still flows through `Encoder::encode(...)` ->
  `encode_with_sink(...)` -> `encode_stream(...)`
- the buffered helper path used by recompress,
  `encode_buffered_pcm_with_sink(...)`, still carries its own
  `encode_buffered_frames(...)` chunk fan-out and ordered write loop

Review implication:
- the encode-speed lane should improve shared internals without teaching the
  docs or tests a second public encode route
- if duplication between `encode_stream(...)` and
  `encode_buffered_pcm_with_sink(...)` becomes worth reducing, that cleanup
  should stay subordinate to the measured hotspot choice instead of turning the
  performance lane into a broad session rewrite
- maintainers should keep documenting `read_pcm_reader(...)`,
  `EncoderConfig::into_encoder(...)`, and `Encoder::encode(...)` as the stable
  conceptual center even if internal helpers move underneath them

### 6. The byte-identity matrix already has repo-local anchors

Repo evidence from `crates/flacx/tests/encode.rs` and `crates/flacx/tests/api.rs`:
- `reference_identity_matrix_repeats_exact_encode_bytes` already covers the
  three throughput-bench fixtures, the `Level0` / `block_size=576` case, the
  variable-block-schedule case, and a metadata-bearing WAV case
- `encoding_speed_verification_lane_keeps_identity_and_perf_gates_bound`
  explicitly binds the benchmark corpus names and identity-matrix case labels
  into the verification story

Review implication:
- the required performance and diff/hash artifacts should reuse that existing
  matrix vocabulary unless execution has a measured reason to extend it
- reviewers should expect benchmark artifacts, output manifests, and test notes
  to line up with those case labels instead of inventing a drifting second
  reference matrix

### 7. The documentation burden is part of the performance work, not a follow-up extra

The PRD already requires:
- a benchmark artifact in
  `.omx/reports/perf/encoding-speed-10pct-identical-output-api-stable.md`
- an optional JSON companion
- an encode-corpus diff / hash artifact in
  `.omx/reports/encode-corpus-diff/encoding-speed-10pct-identical-output-api-stable.json`
- a hotspot timing artifact explaining why Wave 1 was chosen
- an explicit note on whether the first measured wave cleared the full target

Review implication:
- implementation is not complete when the code gets faster but the measurement
  trail is missing
- reviewers should reject performance claims that do not preserve the benchmark
  command, median, target, hotspot evidence, and byte-identity proof together

## Documentation contract for the implementation lane

When this performance work lands, keep the documentation story in this order:

1. **Stable public API first**
   - `read_pcm_reader(...)`
   - `EncoderConfig::into_encoder(...)`
   - `flacx::builtin` remains convenience, not a separate engine
2. **Byte-identity scope second**
   - state the agreed reference corpus/config matrix
   - reuse the repo-local identity-matrix case labels when the artifact covers
     the same benchmark and regression fixtures
   - record output hashes and sizes, not only decode success
   - call out any metadata-bearing fixture used to protect emission order
3. **Measured hotspot explanation third**
   - say which hotspot class actually dominated
   - say why that wave was chosen over the others
4. **Performance evidence fourth**
   - baseline command
   - final command
   - before/after medians
   - percent change
   - encoded-size note if relevant
5. **Regression proof fifth**
   - API tests
   - targeted encode tests
   - feature-matrix checks when family readers are touched
   - byte-identity diff/hash evidence for the reference matrix

Do not describe a broad architecture rewrite if the actual change is a bounded
encode hot-path optimization.

## Review companions

Keep this note paired with:
- `.omx/plans/prd-encoding-speed-10pct-identical-output-api-stable.md`
- `crates/flacx/benches/throughput.rs`
- `crates/flacx/tests/api.rs`
- `crates/flacx/tests/encode.rs`
- `crates/flacx/src/encoder.rs`
- `crates/flacx/src/encode_pipeline.rs`
- `crates/flacx/src/model.rs`
- `crates/flacx/src/wav_input.rs`
- `crates/flacx/src/raw.rs`
- generated artifacts under `.omx/reports/perf/` and
  `.omx/reports/encode-corpus-diff/`

## Exit criteria for this note

This note can shrink or disappear once all of the following are true:

1. the public encode API is still unchanged and verified
2. the throughput bench clears the `>=10%` target against the documented
   planning baseline
3. the current-vs-reference output artifact proves byte-identical outputs for
   the agreed corpus/config matrix
4. the winning hotspot choice is explained by a durable timing artifact
5. any encoded-size movement is recorded instead of implied away
