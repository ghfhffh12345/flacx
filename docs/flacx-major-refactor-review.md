# flacx major refactor review and documentation guide

This maintainer document translates `.omx/plans/prd-flacx-major-refactor.md`
into a code-quality review checklist and a documentation contract for the
explicit-core / convenience-layer refactor.

It is intentionally grounded in the current repo state so implementation,
testing, and docs work can converge on the same vocabulary.

## Goals

- make the low-level encode/decode flow explicit and format-generic
- keep file-path and byte-buffer ergonomics in a first-class convenience layer
- gate supported format families with coarse Cargo features
- keep documentation, tests, and verification lanes aligned with the same
  architecture story

## Grounded review findings

### 1. The public surface is still described from the convenience edge inward

Repo evidence:
- `crates/flacx/src/lib.rs` presents `encode_file`, `encode_bytes`,
  `decode_file`, and `decode_bytes` as the small public API story.
- `crates/flacx/README.md` centers the file helpers and byte helpers before any
  explicit reader/writer or adapter boundary.
- `crates/flacx/src/encoder.rs` and `crates/flacx/src/decode.rs` each mix
  generic stream work, file helpers, and in-memory helpers in the same façade.

Quality implication:
- callers can still understand the crate as a path-oriented convenience API
  instead of an explicit core plus orchestration layer.
- review pressure stays concentrated on the top-level façades because the
  architecture boundary is not yet obvious in the docs.

Documentation requirement:
- describe the core layer first: explicit config, typed PCM handoff,
  format-specific readers/writers, and core encoder/decoder orchestration.
- describe convenience helpers second as thin wrappers that derive config and
  route into the core without duplicating policy.

### 2. Feature-gated format families are not yet visible in the crate contract

Repo evidence:
- `crates/flacx/Cargo.toml` currently exposes only `progress`.
- the codebase already carries family-specific modules such as `aiff.rs`,
  `caf.rs`, `aiff_output.rs`, `caf_output.rs`, and `wav_output.rs`, but the
  public docs do not explain a coarse feature-family contract.

Quality implication:
- supported-format growth is easy to describe as one more hard-coded branch
  instead of a compile-time boundary.
- docs and tests can drift because there is no single feature matrix to point
  at.

Documentation requirement:
- document a small, legible feature surface:
  - `wav` — RIFF/WAVE, RF64, Wave64
  - `aiff` — AIFF, AIFC allowlist
  - `caf` — CAF allowlist
  - `progress` — callback-style progress reporting
- every README/example/test command that depends on a family gate should say so
  explicitly.

### 3. Migration hotspots are concentrated in large container and metadata files

Repo evidence from `wc -l crates/flacx/src/*.rs crates/flacx/tests/*.rs`:
- `crates/flacx/src/metadata.rs` — 1763 lines
- `crates/flacx/src/input.rs` — 1259 lines
- `crates/flacx/src/read.rs` — 1156 lines
- `crates/flacx/src/write.rs` — 884 lines
- container-output modules remain sizable (`wav_output.rs`, `aiff_output.rs`,
  `caf_output.rs`)

Quality implication:
- these files are the highest-risk merge and review zones for the refactor.
- without explicit docs, reviewers may miss whether a change belongs in the
  core, a container adapter, or the convenience layer.

Documentation requirement:
- maintain a short architecture map that names the intended ownership of each
  layer before more code moves happen.
- keep future doc updates biased toward module ownership and policy boundaries,
  not just user-facing examples.

## Target architecture map

### Explicit core

The explicit core should be the source of truth for:
- encode/decode/recompress configuration types
- typed PCM stream or frame handoff between container adapters and FLAC logic
- core encoder/decoder orchestration that works on explicit inputs and outputs
- codec policy, validation, and summary reporting

The explicit core should **not** own:
- file-extension inference
- path-based convenience routing
- ad-hoc policy forks for one-off helpers

### Container adapters

Container readers and writers should own:
- header parsing and emission
- family-specific metadata translation
- exactness / allowlist validation for that family
- conversion to and from the typed PCM abstraction

### Convenience layer

The convenience layer should own:
- file-to-file helpers
- byte-buffer helpers
- extension-based inference when explicitly requested
- safe derivation of core config from user-friendly inputs

Rule: convenience helpers may orchestrate, but they must not reimplement codec
policy that already lives in the core or container adapters.

## Documentation contract for the refactor

When the refactor lands, the docs should tell the same story in this order:

1. **Architecture and feature gates**
   - explicit core
   - convenience layer
   - supported feature families
2. **Core-first examples**
   - explicit encoder/decoder construction
   - explicit container selection where relevant
3. **Convenience examples**
   - file helpers
   - byte helpers
   - extension inference as opt-in behavior
4. **Verification story**
   - targeted tests
   - feature-matrix smoke coverage
   - benchmark lane

Recommended doc surfaces:
- crate-level rustdoc in `crates/flacx/src/lib.rs`
- `crates/flacx/README.md` for public library usage
- workspace `README.md` for the high-level feature story and doc map
- maintainer-only docs for architecture and review watchpoints

## Migration notes for maintainers

Use these notes to review refactor PRs without locking the codebase into final
module names too early.

### Public-story migration

- **Current story:** `Encoder` / `Decoder` plus `encode_file`, `encode_bytes`,
  `decode_file`, and `decode_bytes` read as the primary API.
- **Target story:** explicit encoder/decoder configuration and typed container
  adapters become the primary architecture; path and byte helpers are presented
  as convenience orchestration.

### Ownership migration

- move container-specific parsing/writing expectations out of top-level façade
  documentation and into reader/writer ownership notes
- keep config derivation and extension inference documented as convenience-layer
  behavior rather than core codec behavior
- keep feature-family documentation coarse, stable, and shared across Cargo
  manifests, rustdoc, and README examples

### Review note

During the transition, docs may temporarily mention both the current façade
entry points and the target layered architecture. That is acceptable as long as
the docs make the direction explicit and avoid describing convenience wrappers
as the only conceptual model.

## Verification lanes that docs should name explicitly

The refactor should keep documentation aligned with these lanes:

- targeted regression tests:
  - `cargo test -p flacx --test api --test decode`
- feature matrix smoke:
  - `cargo test -p flacx --no-default-features --features progress,wav`
  - `cargo test -p flacx --no-default-features --features progress,wav,aiff,caf`
- crate diagnostics:
  - `cargo check -p flacx`
- throughput baseline:
  - `cargo bench -p flacx --bench throughput`

If a doc/example depends on a feature family, the verification section should
show a command that exercises the same gate.

## Review checklist for implementation PRs

- Does the doc order present the explicit core before convenience wrappers?
- Are feature families named consistently across Cargo features, README text,
  and test commands?
- Do reader/writer modules own container-specific policy, rather than the top-
  level convenience façades?
- Do convenience helpers delegate to one source of truth instead of branching on
  policy locally?
- Do verification commands cover both feature-on and feature-off behavior?

## Suggested follow-up doc edits after code lands

1. update `crates/flacx/src/lib.rs` rustdoc to lead with the explicit-core story
2. refresh `crates/flacx/README.md` feature examples and supported-format matrix
3. refresh workspace `README.md` dependency examples and documentation map
4. keep benchmark and feature-smoke commands near the architecture notes so the
   performance requirement stays visible during review
